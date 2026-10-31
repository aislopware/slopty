//! What a change reaches of the e2e smoke set (`docs/decisions/testing.md`, "Goldens render in
//! the test process; e2e is a smoke set chosen by what a change reaches", item 5).
//!
//! The smoke set is seven cases ([`SMOKE`]), each named by its tests. A changed path reaches the
//! cases its rule names ([`RULES`]): the app's glue and natives the app's cases (S1 to S3), the
//! daemons and the link every case that runs them, the server and CLI S4, the iOS app S5, and
//! code compiled only for iOS or only for Linux, wherever it sits, S5 or S6 ([`compiled_for`]).
//! What draws (the shell, the views, the theme) reaches none: its goldens render in the test
//! process (`tests::IN_PROCESS` names each such member and why). A workspace member no rule names
//! and none left out fails a test here (`every_member_is_mapped_or_left_out_by_name`), so a new
//! crate is placed when it comes.
//!
//! `cargo xtask smoke reach` says what a change reaches and why; `cargo xtask remote --e2e auto`
//! runs it.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

/// How a smoke case runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Runs {
    /// The app's e2e target (`slopty-e2e`'s `app`), in an e2e guest here or on a runner Mac.
    App,
    /// The simulator's (`ios`), on the iPhone and on the iPad.
    Ios,
    /// The server's (`server`): the CLI and MCP through a real server and worker.
    Server,
    /// A Linux worker in Docker on this Mac (`cargo xtask linux e2e`).
    Linux,
    /// A worker in a macOS guest with its grants (`cargo xtask vm e2e`).
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
        holds: "iOS: one session with its inspector, and one shader frame, on the iPhone and the \
                iPad",
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
/// The app's cases: its glue and natives.
const APP: &[&str] = &["S1", "S2", "S3"];
/// The cases that run the daemons and the link: a session on the stack, the server's worker,
/// the Linux worker and the guest's.
const STACK: &[&str] = &["S1", "S4", "S6", "VM"];
/// [`STACK`] and the stream, which the worker serves and the link carries.
const STREAMED: &[&str] = &["S1", "S2", "S4", "S6", "VM"];

/// Path prefixes (a directory with its `/`, or a file) and the cases a change under one reaches;
/// a path takes every rule it falls under. Code compiled only for iOS or only for Linux reaches
/// S5 or S6 besides, by its `cfg` ([`compiled_for`]).
pub const RULES: &[(&str, &[&str])] = &[
    // What every build reads: the toolchain, the lock, the workspace's manifest and its hack. A
    // lock that only adds or drops packages, and the hack beside it, follow the manifests that
    // asked for them ([`reach`]); one that moves a package reaches every case.
    ("Cargo.lock", ALL),
    ("Cargo.toml", ALL),
    ("rust-toolchain.toml", ALL),
    (".cargo/", ALL),
    ("workspace-hack/", ALL),
    // Vendored code: the terminal's VT and the transport.
    ("vendor/ghostty", STACK),
    ("vendor/noq-", STREAMED),
    // The app's glue.
    ("apps/slopty/", APP),
    ("crates/slopty-app/", APP),
    ("crates/slopty-settings/", &["S1"]),
    ("crates/slopty-crash/", &["S1"]),
    // Its natives: AppKit, WebKit, the PDF view, notifications, the surfaces visuals draw into.
    ("crates/slopty-platform/", &["S1"]),
    ("crates/slopty-platform/src/web", &["S3"]),
    ("crates/slopty-platform/src/pdf.rs", &["S3"]),
    ("crates/slopty-platform/src/notify", &["S5"]),
    ("crates/slopty-platform/src/privacy.rs", &["VM"]),
    ("crates/slopty-visual/", &["S1", "S5"]),
    ("crates/slopty-visual-gpu/", &["S1", "S5"]),
    ("crates/slopty-predict/", &["S1"]),
    // The coding agents' records and hooks, which the inspector reads.
    ("crates/slopty-agent/", &["S1"]),
    ("crates/slopty-tools/", &["S1", "S4"]),
    // The daemons and the link.
    ("apps/slopty-ptyd/", STACK),
    ("crates/slopty-pty/", STACK),
    ("apps/slopty-worker/", STREAMED),
    ("crates/slopty-worker/", STREAMED),
    ("crates/slopty-engine/", STACK),
    ("crates/slopty-grid/", STACK),
    ("crates/slopty-client/", STREAMED),
    ("crates/slopty-net/", STREAMED),
    ("crates/slopty-proto/", STREAMED),
    ("crates/slopty-core/", STREAMED),
    // The stream: VideoToolbox, capture, the virtual display, the media transport.
    ("crates/slopty-codec/", &["S2"]),
    ("crates/slopty-capture/", &["S2", "VM"]),
    ("crates/slopty-vdisplay/", &["S2"]),
    ("crates/slopty-media/", &["S2"]),
    // The worker's input and clipboard seams, and its deploy.
    ("crates/slopty-input/", &["S6", "VM"]),
    ("crates/slopty-deploy/", &["VM"]),
    // The server and the CLI.
    ("apps/slopty-server/", &["S4"]),
    ("crates/slopty-server/", &["S4"]),
    ("apps/slopty-cli/", &["S1", "S4"]),
    // The iOS app.
    ("apps/slopty-ios/", &["S5"]),
    ("apps/slopty-notify/", &["S5"]),
    // The suites themselves: the harness every case runs on, each case's own tests, and the
    // xtask code that runs them.
    ("crates/slopty-e2e/Cargo.toml", ALL),
    ("crates/slopty-e2e/build.rs", ALL),
    ("crates/slopty-e2e/src/", ALL),
    ("crates/slopty-testkit/", ALL),
    ("crates/slopty-e2e/tests/app.rs", APP),
    ("crates/slopty-e2e/tests/ios.rs", &["S5"]),
    ("crates/slopty-e2e/tests/server.rs", &["S4"]),
    ("crates/slopty-e2e/tests/linux.rs", &["S6"]),
    ("crates/slopty-e2e/tests/vm.rs", &["VM"]),
    ("xtask/src/e2e.rs", &["S1", "S2", "S3", "S4", "S5"]),
    ("xtask/src/ios.rs", &["S5"]),
    ("xtask/src/linux.rs", &["S6"]),
    ("xtask/src/vm.rs", &["VM"]),
];

/// Lines of a changed file: all of it (added or removed whole), or these (1-based, in the
/// version read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lines {
    /// The whole file.
    All,
    /// These lines.
    Some(Vec<usize>),
}

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

    /// The nextest filterset of the reached cases that run as `runs`, `None` when none does.
    pub fn filterset(&self, runs: Runs) -> Option<String> {
        let tests: Vec<String> = self
            .cases()
            .filter(|smoke| smoke.runs == runs)
            .flat_map(|smoke| smoke.tests.iter().map(|test| format!("test(={test})")))
            .collect();
        (!tests.is_empty()).then(|| tests.join(" | "))
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

/// The cases a change to `path` reaches: its rules', and S5 or S6 for its changed `lines`
/// compiled only for iOS or only for Linux. `read` gives a file's text at the change (`None`
/// for none): the changed file's own, and its parents' module declarations.
pub fn reached(
    path: &str,
    lines: &Lines,
    read: &dyn Fn(&str) -> Option<String>,
) -> BTreeSet<&'static str> {
    let mut ids: BTreeSet<&'static str> = RULES
        .iter()
        .filter(|(prefix, _)| under(path, prefix))
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    // A smoke test's own module reaches its case.
    for runs in [Runs::App, Runs::Ios] {
        let dir = format!("crates/slopty-e2e/tests/{}/", runs.target());
        if let Some(module) = path.strip_prefix(&dir).and_then(|m| m.strip_suffix(".rs")) {
            for smoke in SMOKE.iter().filter(|s| s.runs == runs) {
                if smoke.tests.iter().any(|t| t.split("::").next() == Some(module)) {
                    let _new = ids.insert(smoke.id);
                }
            }
        }
    }
    if Some("rs") == std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        let only = compiled_for(path, lines, read);
        if only.contains(&"ios") {
            let _new = ids.insert("S5");
        }
        if only.contains(&"linux") {
            let _new = ids.insert("S6");
        }
    }
    ids
}

/// Whether `path` is `prefix` or under it.
fn under(path: &str, prefix: &str) -> bool {
    let file = prefix.rsplit('/').next().is_some_and(|last| last.contains('.'));
    path == prefix || (!file && path.starts_with(prefix))
}

/// The operating systems (`ios`, `linux`) that alone compile some of `lines` of the Rust file
/// `path`: lines inside an item, statement or block under a `#[cfg(..)]` that asks for that
/// `target_os` (`not(..)` turning it round), a file whose own `#![cfg(..)]` does, or a module
/// file whose `mod` is declared so in its parent, or its parent's parent.
pub fn compiled_for(
    path: &str,
    lines: &Lines,
    read: &dyn Fn(&str) -> Option<String>,
) -> BTreeSet<&'static str> {
    let mut only = BTreeSet::new();
    let Some(text) = read(path) else { return only };
    for os in ["ios", "linux"] {
        let whole = file_only_for(&text, os) || module_only_for(path, os, read, 0);
        let hit = whole
            || match lines {
                Lines::All => !spans(&text, os).is_empty(),
                Lines::Some(lines) => {
                    let spans = spans(&text, os);
                    lines.iter().any(|line| spans.iter().any(|(a, b)| (a..=b).contains(&line)))
                }
            };
        if hit {
            let _new = only.insert(os);
        }
    }
    only
}

/// Whether the file's inner `#![cfg(..)]` asks for `os`.
fn file_only_for(text: &str, os: &str) -> bool {
    text.lines()
        .map(str::trim)
        .take_while(|l| l.is_empty() || l.starts_with("//") || l.starts_with("#!"))
        .filter_map(|l| l.strip_prefix("#![cfg(").and_then(|l| l.strip_suffix(")]")))
        .any(|pred| asks_for(pred, os))
}

/// Whether the module file `path` is declared under a `cfg` asking for `os` in its parent
/// module's file, or that one is in its own (at most `depth` 8 up).
fn module_only_for(
    path: &str,
    os: &str,
    read: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Some((parent, name)) = parent_module(path, read) else { return false };
    let Some(text) = read(&parent) else { return false };
    let declared = spans(&text, os).iter().any(|&(start, end)| {
        text.lines()
            .skip(start.saturating_sub(1))
            .take(end.saturating_sub(start).saturating_add(1))
            .any(|l| declares(l, &name))
    });
    declared
        || file_only_for(&text, os)
        || module_only_for(&parent, os, read, depth.saturating_add(1))
}

/// Whether `line` declares the module `name` (`mod name;`, `pub mod name;`, `pub(crate) mod
/// name;`).
fn declares(line: &str, name: &str) -> bool {
    let line = line.trim();
    let line =
        line.strip_prefix("pub(crate) ").or_else(|| line.strip_prefix("pub ")).unwrap_or(line);
    line.strip_prefix("mod ").and_then(|l| l.strip_suffix(';')).is_some_and(|l| l.trim() == name)
}

/// The file declaring the module file `path`, and the module's name: `a/b.rs` and `a/b/mod.rs`
/// are declared in `a.rs`, `a/mod.rs`, or the crate's `lib.rs` or `main.rs` beside them; a crate
/// root has none.
fn parent_module(path: &str, read: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let (dir, file) = path.rsplit_once('/')?;
    if matches!(file, "lib.rs" | "main.rs") {
        return None;
    }
    let (dir, name) = if file == "mod.rs" {
        let (up, name) = dir.rsplit_once('/')?;
        (up, name.to_owned())
    } else {
        (dir, file.strip_suffix(".rs")?.to_owned())
    };
    let candidates = match dir.rsplit_once('/') {
        Some((up, last)) if last != "src" => {
            vec![format!("{up}/{last}.rs"), format!("{dir}/mod.rs")]
        }
        _ => vec![format!("{dir}/lib.rs"), format!("{dir}/main.rs")],
    };
    candidates.into_iter().find(|c| read(c).is_some()).map(|c| (c, name))
}

/// The 1-based line spans of `text`'s items, statements and blocks under a `#[cfg(..)]` that
/// asks for `os`: from the attribute to the `;` or the closing brace that ends what it is on, or
/// else to the end of the list or block it is in (a field, an argument, an arm: wider than the
/// thing, never narrower, as a comma also parts generics and `where` clauses). Strings,
/// characters and comments are skipped; a bracket in them does not count.
fn spans(text: &str, os: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let (mut i, mut line, mut depth) = (0_usize, 1_usize, 0_usize);
    // Open spans: the line its attribute is on, the depth it is at, and whether its item opened
    // a brace yet.
    let mut open: Vec<(usize, usize, bool)> = Vec::new();
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\n' => line = line.saturating_add(1),
            b'/' if bytes.get(i.saturating_add(1)) == Some(&b'/') => {
                while bytes.get(i).is_some_and(|&c| c != b'\n') {
                    i = i.saturating_add(1);
                }
                continue;
            }
            b'/' if bytes.get(i.saturating_add(1)) == Some(&b'*') => {
                i = i.saturating_add(2);
                while let Some(&c) = bytes.get(i) {
                    if c == b'\n' {
                        line = line.saturating_add(1);
                    }
                    if c == b'*' && bytes.get(i.saturating_add(1)) == Some(&b'/') {
                        i = i.saturating_add(1);
                        break;
                    }
                    i = i.saturating_add(1);
                }
            }
            b'"' => {
                i = i.saturating_add(1);
                while let Some(&c) = bytes.get(i) {
                    match c {
                        b'\\' => i = i.saturating_add(1),
                        b'\n' => line = line.saturating_add(1),
                        b'"' => break,
                        _ => {}
                    }
                    i = i.saturating_add(1);
                }
            }
            b'r' if bytes.get(i.saturating_add(1)).is_some_and(|&c| c == b'#' || c == b'"')
                && !bytes
                    .get(i.wrapping_sub(1))
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') =>
            {
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
            b'#' if bytes.get(i.saturating_add(1)) == Some(&b'[') => {
                let end = attribute_end(bytes, i.saturating_add(1));
                let attribute = String::from_utf8_lossy(bytes.get(i..end).unwrap_or_default());
                if let Some(pred) =
                    attribute.strip_prefix("#[cfg(").and_then(|a| a.strip_suffix(")]"))
                    && asks_for(pred, os)
                {
                    open.push((line, depth, false));
                }
                line = line.saturating_add(attribute.matches('\n').count());
                i = end;
                continue;
            }
            b'{' | b'(' | b'[' => {
                if b == b'{'
                    && let Some(last) = open.last_mut()
                    && last.1 == depth
                {
                    last.2 = true;
                }
                depth = depth.saturating_add(1);
            }
            b'}' | b')' | b']' => {
                depth = depth.saturating_sub(1);
                while let Some(&(start, at, braced)) = open.last() {
                    if (b == b'}' && braced && at == depth) || at > depth {
                        spans.push((start, line));
                        let _closed = open.pop();
                    } else {
                        break;
                    }
                }
            }
            b';' => {
                // `mod x;`, `use ..;`, a statement.
                while let Some(&(start, at, braced)) = open.last() {
                    if !braced && at == depth {
                        spans.push((start, line));
                        let _closed = open.pop();
                    } else {
                        break;
                    }
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    spans
}

/// The index past the `]` closing the attribute whose `[` is at `open`.
fn attribute_end(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0_usize;
    let mut i = open;
    while let Some(&c) = bytes.get(i) {
        match c {
            b'[' => depth = depth.saturating_add(1),
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i.saturating_add(1);
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    bytes.len()
}

/// Whether the `cfg` predicate `pred` asks for `target_os = "<os>"`: names it outside any
/// `not(..)`, or inside two.
pub fn asks_for(pred: &str, os: &str) -> bool {
    let wanted = format!("target_os = \"{os}\"");
    let mut negations: Vec<bool> = Vec::new();
    let mut i = 0_usize;
    while let Some(rest) = pred.get(i..)
        && !rest.is_empty()
    {
        if rest.starts_with(&wanted) {
            if negations.iter().filter(|&&n| n).count() % 2 == 0 {
                return true;
            }
            i = i.saturating_add(wanted.len());
            continue;
        }
        if rest.starts_with("not(") {
            negations.push(true);
            i = i.saturating_add(4);
            continue;
        }
        match rest.as_bytes().first() {
            Some(b'(') => negations.push(false),
            Some(b')') => {
                let _closed = negations.pop();
            }
            _ => {}
        }
        i = i.saturating_add(rest.chars().next().map_or(1, char::len_utf8));
    }
    false
}

/// What the change from `base` to `head` reaches: every path the diff names (renames as a removal
/// and an addition), with the changed lines of each Rust file.
pub fn reach(sh: &Shell, base: &str, head: &str) -> Result<Reach> {
    let names = cmd!(sh, "git diff --no-renames --name-status {base} {head}")
        .quiet()
        .read()
        .context("the change's paths")?;
    let at = |rev: &str, path: &str| -> Option<String> {
        cmd!(sh, "git show {rev}:{path}").quiet().ignore_stderr().read().ok()
    };
    let mut reach = Reach::default();
    let moved = lock_moved(
        &at(base, "Cargo.lock").unwrap_or_default(),
        &at(head, "Cargo.lock").unwrap_or_default(),
    );
    for row in names.lines() {
        let Some((status, path)) = row.split_once('\t') else { continue };
        if !moved && (path == "Cargo.lock" || path.starts_with("workspace-hack/")) {
            continue;
        }
        // A removed file is read as it was, whole; any other at `head`, at its changed lines.
        let (rev, lines) = if status.starts_with('D') {
            (base, Lines::All)
        } else if status.starts_with('A')
            || std::path::Path::new(path).extension().is_none_or(|e| e != "rs")
        {
            (head, Lines::All)
        } else {
            let hunks =
                cmd!(sh, "git diff -U0 --no-color {base} {head} -- {path}").quiet().read()?;
            (head, Lines::Some(changed_lines(&hunks)))
        };
        let read = |p: &str| at(rev, p);
        reach.add(reached(path, &lines, &read), path);
    }
    Ok(reach)
}

/// Whether the lock `new` moved a package `old` had (another version or source of one it keeps
/// the name of), rather than only adding or dropping some: a new dependency takes its own, and
/// what it reaches is its manifest's crate's.
///
/// A lock that does not parse is taken for moved: what it holds is unknown, so it reaches every
/// case rather than none.
fn lock_moved(old: &str, new: &str) -> bool {
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
    let (Some(old), Some(new)) = (packages(old), packages(new)) else { return true };
    let names: BTreeSet<&str> = new.iter().map(|(name, ..)| name.as_str()).collect();
    old.iter().any(|p| !new.contains(p) && names.contains(p.0.as_str()))
}

/// The lines of the new side a `git diff -U0` changed, from its hunk headers; a hunk that only
/// removes names the line it removed after.
fn changed_lines(diff: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for header in diff.lines().filter(|l| l.starts_with("@@ ")) {
        let Some(new) = header.split_whitespace().find(|w| w.starts_with('+')) else { continue };
        let mut parts = new.trim_start_matches('+').splitn(2, ',');
        let start: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        if count == 0 {
            lines.push(start.max(1));
        } else {
            lines.extend(start..start.saturating_add(count));
        }
    }
    lines
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Lines, Runs};

    /// The workspace's members that reach no smoke case, and why: each is held by tests in the
    /// test process, so no e2e run is the first to see its change.
    const IN_PROCESS: &[(&str, &str)] = &[
        ("crates/slopty-shell", "the shell's views: goldens rendered in the test process"),
        ("crates/slopty-ui", "the views and elements: goldens rendered in the test process"),
        ("crates/slopty-kit", "the chrome kit: in-process tests and goldens"),
        ("crates/slopty-terminal", "the terminal view: goldens rendered in the test process"),
        ("crates/slopty-theme", "design tokens: in-process goldens"),
        ("crates/slopty-shell-theme", "the theme model: in-process goldens"),
        ("crates/slopty-markdown", "UI-free markdown: unit and property tests"),
        ("crates/slopty-math", "math layout: unit tests and goldens"),
        ("crates/slopty-syntax", "highlighting with no UI: unit tests"),
        ("crates/slopty-veil", "the fade veil without a renderer: unit tests"),
        ("crates/slopty-view-doc", "the view model's shapes: fixture tests"),
        ("crates/slopty-view-proto", "the view model's wire types: unit tests"),
        ("crates/slopty-view-rpc", "the view model's calls: in-process tests"),
        ("crates/slopty-dnd", "drag and drop: its own live tests"),
        ("crates/slopty-push", "the APNs path: its own tests; no smoke case sends a push"),
        ("crates/slopty-shape", "a test-only link shaper"),
        ("crates/slopty-tailnet", "the Tailscale LocalAPI: model tests against its fake"),
        ("apps/slopty-files", "the File Provider extension: no smoke case opens Finder"),
    ];

    fn reach(path: &str, lines: &Lines, files: &[(&str, &str)]) -> BTreeSet<&'static str> {
        let read = |p: &str| files.iter().find(|(f, _)| *f == p).map(|(_, t)| (*t).to_owned());
        super::reached(path, lines, &read)
    }

    /// The ruling's map: the app's glue and natives reach the app's cases, the server and CLI
    /// S4, the iOS app S5, the daemons every case that runs them, the lock everything, and what
    /// draws nothing.
    #[test]
    fn a_path_reaches_the_cases_its_rule_names() {
        let ids = |path| reach(path, &Lines::All, &[]);
        assert_eq!(ids("crates/slopty-app/src/workers.rs"), ["S1", "S2", "S3"].into());
        assert_eq!(ids("crates/slopty-platform/src/web/page/sandbox.rs"), ["S1", "S3"].into());
        assert_eq!(ids("crates/slopty-codec/src/lib.rs"), ["S2"].into());
        assert_eq!(ids("apps/slopty-server/src/main.rs"), ["S4"].into());
        assert_eq!(ids("apps/slopty-ios/src/lib.rs"), ["S5"].into());
        assert_eq!(ids("apps/slopty-ptyd/src/daemon.rs"), ["S1", "S4", "S6", "VM"].into());
        assert_eq!(ids("Cargo.lock"), super::ALL.iter().copied().collect());
        assert_eq!(ids("crates/slopty-shell/src/shell/chrome.rs"), BTreeSet::new());
        assert_eq!(ids("docs/TESTING.md"), BTreeSet::new());
        // A prefix is a directory or a file, never the start of another name.
        assert_eq!(ids("Cargo.toml.orig"), BTreeSet::new());
        assert_eq!(ids("crates/slopty-application/src/lib.rs"), BTreeSet::new());
    }

    /// A smoke test's own module reaches its cases, and the other modules of its target none.
    #[test]
    fn a_smoke_test_s_module_reaches_its_case() {
        let ids = |path| reach(path, &Lines::All, &[]);
        assert_eq!(ids("crates/slopty-e2e/tests/app/media.rs"), ["S1", "S3"].into());
        assert_eq!(ids("crates/slopty-e2e/tests/app/surfaces_live.rs"), ["S1", "S2"].into());
        assert_eq!(ids("crates/slopty-e2e/tests/app/transcript.rs"), BTreeSet::new());
        assert_eq!(ids("crates/slopty-e2e/tests/ios/screens.rs"), ["S5"].into());
        assert_eq!(ids("crates/slopty-e2e/tests/ios/compact.rs"), BTreeSet::new());
    }

    /// Code compiled only for iOS reaches S5 wherever it sits, and only for Linux S6: a changed
    /// line under such a `cfg`, not one beside it; `not(..)` turns it round.
    #[test]
    fn code_compiled_for_one_os_reaches_its_case() {
        let file = "crates/slopty-ui/src/screen.rs";
        let text = "\
use gpui::App;

#[cfg(target_os = \"ios\")]
fn soft_keyboard() {
    let brace = '{';
    let s = \"}\";
}

#[cfg(not(target_os = \"ios\"))]
fn hardware_keyboard() {}

#[cfg(any(target_os = \"linux\", target_os = \"freebsd\"))]
mod inotify;

fn shared() {
    #[cfg(target_os = \"ios\")]
    let only = 1;
    let both = 2;
}
";
        let files = [(file, text)];
        let at = |lines: &[usize]| reach(file, &Lines::Some(lines.to_vec()), &files);
        assert_eq!(at(&[1]), BTreeSet::new());
        assert_eq!(at(&[5]), ["S5"].into(), "inside the iOS function, past a brace in quotes");
        assert_eq!(at(&[7]), ["S5"].into(), "its closing brace");
        assert_eq!(at(&[10]), BTreeSet::new(), "not(ios)");
        assert_eq!(at(&[13]), ["S6"].into(), "any(linux, ..)");
        assert_eq!(at(&[17]), ["S5"].into(), "an iOS statement");
        assert_eq!(at(&[18]), BTreeSet::new(), "the statement after it");
    }

    /// A module file is iOS-only when its parent declares it so, or when it says so itself.
    #[test]
    fn a_module_declared_for_ios_is_ios_code() {
        let files = [
            (
                "crates/slopty-platform/src/web.rs",
                "#[cfg(target_os = \"ios\")]\nmod ios;\nmod page;\n",
            ),
            ("crates/slopty-platform/src/web/ios.rs", "pub fn view() {}\n"),
            ("crates/slopty-platform/src/web/page.rs", "pub fn card() {}\n"),
            ("crates/slopty-platform/src/web/page/view.rs", "pub fn draw() {}\n"),
            ("crates/slopty-platform/src/lib.rs", "pub mod web;\n"),
            (
                "apps/slopty-notify/src/lib.rs",
                "//! The extension.\n#![cfg(target_os = \"ios\")]\nfn x() {}\n",
            ),
        ];
        let os = |path| {
            let read = |p: &str| files.iter().find(|(f, _)| *f == p).map(|(_, t)| (*t).to_owned());
            super::compiled_for(path, &Lines::Some(vec![1]), &read)
        };
        assert_eq!(os("crates/slopty-platform/src/web/ios.rs"), ["ios"].into());
        assert_eq!(os("crates/slopty-platform/src/web/page.rs"), BTreeSet::new());
        assert_eq!(os("crates/slopty-platform/src/web/page/view.rs"), BTreeSet::new());
        assert_eq!(os("apps/slopty-notify/src/lib.rs"), ["ios"].into());
    }

    /// `cfg` predicates: `target_os` asked for plainly, in `any` or `all`, or under two `not`s;
    /// not under one.
    #[test]
    fn a_cfg_asks_for_an_os_outside_a_not() {
        assert!(super::asks_for("target_os = \"ios\"", "ios"));
        assert!(super::asks_for("any(target_os = \"macos\", target_os = \"ios\")", "ios"));
        assert!(super::asks_for("all(test, target_os = \"ios\")", "ios"));
        assert!(super::asks_for("not(not(target_os = \"ios\"))", "ios"));
        assert!(!super::asks_for("not(target_os = \"ios\")", "ios"));
        assert!(!super::asks_for("all(unix, not(target_os = \"ios\"))", "ios"));
        assert!(!super::asks_for("target_os = \"macos\"", "ios"));
    }

    /// A lock moves when a package it had takes another version or source, not when packages
    /// come or go.
    #[test]
    fn a_lock_moves_when_a_package_it_had_changes() {
        let lock = |packages: &[(&str, &str)]| {
            packages.iter().fold(String::from("version = 4\n"), |lock, (name, version)| {
                format!("{lock}\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n")
            })
        };
        let old = lock(&[("tokio", "1.50.0"), ("serde", "1.0.229")]);
        assert!(!super::lock_moved(
            &old,
            &lock(&[("tokio", "1.50.0"), ("serde", "1.0.229"), ("wgpu", "29.0.0")])
        ));
        assert!(!super::lock_moved(&old, &lock(&[("tokio", "1.50.0")])));
        assert!(super::lock_moved(&old, &lock(&[("tokio", "1.51.0"), ("serde", "1.0.229")])));
    }

    /// A hunk header's new lines, and a removal's place.
    #[test]
    fn a_diff_s_hunks_name_their_new_lines() {
        let diff =
            "diff --git a/x.rs b/x.rs\n@@ -3 +3 @@\n-a\n+b\n@@ -10,2 +11,3 @@\n@@ -20,4 +22,0 @@\n";
        assert_eq!(super::changed_lines(diff), [3, 11, 12, 13, 22]);
    }

    /// A reached case runs its own tests alone, in the target that holds them.
    #[test]
    fn a_reached_case_runs_its_tests_alone() {
        let mut reach = super::Reach::default();
        reach.add(["S1", "S3", "S4", "S6"], "crates/slopty-app/src/lib.rs");
        assert_eq!(
            reach.filterset(Runs::App).as_deref().map(|f| f.matches("test(=").count()),
            Some(12),
            "S1's nine and S3's three"
        );
        assert_eq!(
            reach.filterset(Runs::Server).as_deref().map(|f| f.matches("test(=").count()),
            Some(2)
        );
        assert_eq!(reach.filterset(Runs::Ios), None);
    }

    /// A lock that does not parse reaches every case, as a moved one does.
    #[test]
    fn a_lock_that_does_not_parse_is_taken_for_moved() {
        let lock = "version = 4\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n";
        assert!(!super::lock_moved(lock, lock), "the same lock moved nothing");
        assert!(super::lock_moved(lock, "[[package]\n"), "an unreadable lock moved everything");
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

    /// Every workspace member is placed: under a rule, or left out by name with its reason.
    #[test]
    fn every_member_is_mapped_or_left_out_by_name() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for group in ["crates", "apps"] {
            for entry in std::fs::read_dir(root.join(group)).unwrap() {
                let entry = entry.unwrap();
                if !entry.path().join("Cargo.toml").exists() {
                    continue;
                }
                let name = entry.file_name().into_string().unwrap();
                let member = format!("{group}/{name}");
                let ruled = super::RULES
                    .iter()
                    .any(|(prefix, _)| prefix.starts_with(&format!("{member}/")));
                let left = IN_PROCESS.iter().any(|(path, _)| *path == member);
                assert!(ruled ^ left, "{member}: placed by a rule xor left out by name");
            }
        }
    }
}
