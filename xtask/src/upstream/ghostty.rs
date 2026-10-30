//! The ghostty fork, `vendor/ghostty`, and the pin libghostty-rs carries for it.
//!
//! `vendor/ghostty` is what every build compiles (`GHOSTTY_SOURCE_DIR`), so a sync never merges
//! in it: a conflict would leave markers in the tree every session builds from. The merge runs
//! in a linked worktree of the same repository ([`WORKTREE`], holding the fork's branch), and
//! `vendor/ghostty` itself only ever moves, detached, to a head the fork already has.
//!
//! The binding pins one ghostty commit (`GHOSTTY_COMMIT` in libghostty-vt-sys's build script,
//! fetched from `GHOSTTY_REPO` when no source directory is given) and checks in bindings
//! generated from that commit's `include/ghostty`. [`repin`] moves both in one commit.

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::{Fork, same_repo, short};
use crate::tools::step;

/// Where the fork's branch is checked out for a merge, relative to the main checkout.
pub const WORKTREE: &str = ".research/ghostty";
/// libghostty-vt-sys's build script, relative to the libghostty-rs checkout.
const BUILD_RS: &str = "crates/libghostty-vt-sys/build.rs";
/// What the binding's `gen-bindings` writes from ghostty's headers, relative to the
/// libghostty-rs checkout: the declarations every target shares, and each target's layout checks.
const GENERATED: [&str; 2] =
    ["crates/libghostty-vt-sys/src/bindings.rs", "crates/libghostty-vt-sys/src/layouts.rs"];
const COMMIT_LINE: &str = "const GHOSTTY_COMMIT: &str = \"";
const REPO_LINE: &str = "const GHOSTTY_REPO: &str = \"";

/// The value of the one `const <prefix>…";` line in a build script.
fn constant<'a>(script: &'a str, prefix: &str) -> Result<&'a str> {
    let mut values =
        script.lines().filter_map(|line| line.trim().strip_prefix(prefix)?.split('"').next());
    let value = values.next().with_context(|| format!("build.rs has no `{prefix}…\"`"))?;
    ensure!(values.next().is_none(), "build.rs has `{prefix}…\"` twice");
    Ok(value)
}

/// The ghostty commit a build script pins.
pub fn pinned(script: &str) -> Result<&str> {
    constant(script, COMMIT_LINE)
}

/// The build script with its pin moved to `commit`. The script must fetch from `fork`, or the
/// pin would name a commit its repository does not have.
fn repinned(script: &str, commit: &str, fork: &str) -> Result<String> {
    let repo = constant(script, REPO_LINE)?;
    ensure!(same_repo(repo, fork), "build.rs fetches ghostty from {repo}, not the fork {fork}");
    ensure!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "{commit:?} is not a full commit id"
    );
    let old = pinned(script)?;
    Ok(script.replacen(&format!("{COMMIT_LINE}{old}\""), &format!("{COMMIT_LINE}{commit}\""), 1))
}

/// Make [`WORKTREE`] ready to merge upstream into the fork's branch, returning its path.
/// `vendor/ghostty` is initialised when missing and detached where it stands (no file changes),
/// since a branch is checked out in one worktree at a time.
pub fn prepare(sh: &Shell, main: &Utf8Path, fork: &Fork) -> Result<Utf8PathBuf> {
    let vendor = main.join(&fork.checkout);
    if !vendor.join(".git").exists() {
        let _dir = sh.push_dir(main);
        let checkout = &fork.checkout;
        step(
            "init vendor/ghostty",
            &cmd!(sh, "git submodule update --init --filter=blob:none -- {checkout}"),
        )?;
    }
    let _dir = sh.push_dir(&vendor);
    let branch = &fork.branch;
    let on = cmd!(sh, "git symbolic-ref --quiet --short HEAD").ignore_stderr().read().ok();
    if on.as_deref() == Some(branch.as_str()) {
        cmd!(sh, "git switch --quiet --detach").run()?;
        println!("  {} detached at its commit so {WORKTREE} can hold {branch}", fork.checkout);
    }
    let worktree = main.join(WORKTREE);
    if !worktree.join(".git").exists() {
        ensure!(!worktree.exists(), "{worktree} exists but is not a worktree of {vendor}");
        cmd!(sh, "git worktree prune").run()?;
        step("worktree", &cmd!(sh, "git worktree add --quiet --detach {worktree}"))?;
    }
    Ok(worktree)
}

/// Fail unless `commit` is on the fork's branch: a pin must name a commit its repository has.
pub fn ensure_on_fork(sh: &Shell, dir: &Utf8Path, fork: &Fork, commit: &str) -> Result<()> {
    let _dir = sh.push_dir(dir);
    let head = super::fetch(sh, &fork.url, &fork.branch)?;
    let tip = &head.sha;
    let on_fork = cmd!(sh, "git merge-base --is-ancestor {commit} {tip}").quiet().run().is_ok();
    ensure!(
        on_fork,
        "ghostty {} is not on {} {}; run `cargo xtask upstream sync --only ghostty`",
        short(commit),
        fork.url,
        fork.branch
    );
    Ok(())
}

/// Move `vendor/ghostty` to `head`, detached. It must be clean: the tree every build compiles
/// is not the place to lose an edit.
pub fn move_submodule(sh: &Shell, main: &Utf8Path, fork: &Fork, head: &str) -> Result<()> {
    let vendor = main.join(&fork.checkout);
    let _dir = sh.push_dir(&vendor);
    super::ensure_idle(sh, &vendor)?;
    step(
        &format!("move {} to {}", fork.checkout, short(head)),
        &cmd!(sh, "git checkout --quiet --detach {head}"),
    )?;
    let now = cmd!(sh, "git rev-parse HEAD").read()?;
    ensure!(now == head, "{vendor} is at {} after the move, not {}", short(&now), short(head));
    Ok(())
}

/// The command that regenerates libghostty-rs's bindings from the ghostty `GHOSTTY_SOURCE_DIR`
/// names. It compiles, so it runs on the sync's runner (`super::check_on_runner`). bindgen parses
/// the headers once per target with LLVM's own libclang, as upstream's dev shell does: Xcode's
/// clang sends a musl target's `stddef.h` on to a system header a freestanding parse has none of.
pub const REGENERATE: &str = "brew list --formula llvm >/dev/null 2>&1 || brew install --quiet llvm\n\
     LIBCLANG_PATH=\"$(brew --prefix llvm)/lib\" \
     cargo run --locked --quiet -p libghostty-vt-sys --features bindgen-tool --bin gen-bindings";

/// Pin libghostty-rs (checked out at `dir`, on its branch) to ghostty `head` and commit it. The
/// bindings are regenerated on the runner, and [`take_bindings`] adds them to this commit.
/// Nothing happens when the pin is already `head`.
pub fn repin(sh: &Shell, dir: &Utf8Path, fork: &str, head: &str) -> Result<()> {
    let _dir = sh.push_dir(dir);
    let path = dir.join(BUILD_RS);
    let script = sh.read_file(&path)?;
    let old = pinned(&script)?.to_owned();
    if old == head {
        println!("  binding already pins ghostty {}", short(head));
        return Ok(());
    }
    sh.write_file(&path, repinned(&script, head, fork)?)?;
    ensure!(pinned(&sh.read_file(&path)?)? == head, "{path} did not take the new pin");
    cmd!(sh, "git add -- {BUILD_RS}").run()?;
    let subject = format!("build: pin ghostty {}", short(head));
    let body = format!("Moves GHOSTTY_COMMIT from {} to {} on {}.", short(&old), short(head), fork);
    step(&subject, &cmd!(sh, "git commit --quiet -m {subject} -m {body}"))?;
    let staged = cmd!(sh, "git status --porcelain").read()?;
    if !staged.is_empty() {
        bail!("{dir} still has changes after the pin commit:\n{staged}");
    }
    Ok(())
}

/// Commit the bindings the runner regenerated (`patch`, a diff on the head it checked) onto
/// libghostty-rs at `dir`: folded into the pin commit [`repin`] made, or a commit of their own
/// when the pin did not move. An empty patch changes nothing.
pub fn take_bindings(sh: &Shell, dir: &Utf8Path, patch: &Utf8Path) -> Result<()> {
    let _dir = sh.push_dir(dir);
    if std::fs::metadata(patch)?.len() == 0 {
        println!("  bindings unchanged");
        return Ok(());
    }
    cmd!(sh, "git apply --index {patch}").run().context("applying the regenerated bindings")?;
    let changed = cmd!(sh, "git diff --cached --name-only").read()?;
    let stray = strays(&changed);
    ensure!(
        stray.is_empty(),
        "the runner's patch changes what gen-bindings does not write: {stray:?}"
    );
    let last = cmd!(sh, "git log -1 --format=%s").read()?;
    if let Some(pin) = last.strip_prefix("build: pin ghostty ") {
        let subject = format!("build: pin ghostty {pin} and regenerate bindings");
        let body = cmd!(sh, "git log -1 --format=%b").read()?;
        step(&subject, &cmd!(sh, "git commit --quiet --amend -m {subject} -m {body}"))?;
    } else {
        let subject = "build: regenerate the bindings";
        step(subject, &cmd!(sh, "git commit --quiet -m {subject}"))?;
    }
    Ok(())
}

/// The files of `changed` (one path a line) that are none of what `gen-bindings` writes.
fn strays(changed: &str) -> Vec<&str> {
    changed.lines().filter(|file| !GENERATED.contains(file)).collect()
}

/// The commit this repository's `HEAD` records for the submodule at `checkout`.
pub fn recorded(sh: &Shell, root: &Utf8Path, checkout: &Utf8Path) -> Result<String> {
    let _dir = sh.push_dir(root);
    let entry = cmd!(sh, "git ls-tree HEAD -- {checkout}").read()?;
    Ok(entry.split_whitespace().nth(2).unwrap_or_default().to_owned())
}

/// For `check`: where `vendor/ghostty` stands against the fork head and the commit this
/// repository records for it, and which upstream commits since the base touch the paths
/// libghostty-vt builds from.
pub fn report(
    sh: &Shell,
    root: &Utf8Path,
    fork: &Fork,
    fork_head: &str,
    upstream: &str,
) -> Result<()> {
    let checkout = &fork.checkout;
    let recorded = recorded(sh, root, checkout)?;
    let head = cmd!(sh, "git rev-parse HEAD").read()?;
    let state = if head == fork_head { "= fork head" } else { "≠ fork head" };
    println!(
        "  {checkout} at {} {state} {}; this repository records {}",
        short(&head),
        short(fork_head),
        short(&recorded)
    );
    let paths = &fork.tracking.paths;
    if paths.is_empty() {
        return Ok(());
    }
    let range = format!("{}..{upstream}", fork.tracking.base);
    let format = "--format=%h %cs %s";
    let log = cmd!(sh, "git log {format} {range} -- {paths...}").read()?;
    let lines: Vec<&str> = log.lines().collect();
    if lines.is_empty() {
        return Ok(());
    }
    println!("  {} of them touch {}:", lines.len(), paths.join(" "));
    for line in lines.iter().take(SHOWN) {
        println!("    {line}");
    }
    if lines.len() > SHOWN {
        println!("    … and {} more", lines.len().saturating_sub(SHOWN));
    }
    Ok(())
}

/// How many upstream commits `report` lists.
const SHOWN: usize = 40;

#[cfg(test)]
mod tests {
    use super::{GENERATED, pinned, repinned, strays};

    const FORK: &str = "https://github.com/aislopware/ghostty.git";
    const OLD: &str = "7d0734aa89a85174bcf60a0c0626a6b8c2fb0bea";
    const NEW: &str = "0538f7535be0cbca6bbe54e6fde654d5c628f1f2";

    fn script(repo: &str) -> String {
        format!(
            "use std::env;\n\n/// Pinned ghostty commit.\nconst GHOSTTY_REPO: &str = \"{repo}\";\n\
             const GHOSTTY_COMMIT: &str = \"{OLD}\";\n\nfn main() {{\n    \
             eprintln!(\"Fetching ghostty {{GHOSTTY_COMMIT}} ...\");\n}}\n"
        )
    }

    #[test]
    fn the_pin_moves_and_nothing_else() {
        let before = script(FORK);
        let after = repinned(&before, NEW, "git@github.com:aislopware/ghostty").expect("repin");
        assert_eq!(pinned(&after).ok(), Some(NEW), "the new pin");
        assert_eq!(after.replace(NEW, OLD), before, "only the pin changed");
    }

    #[test]
    fn a_pin_is_refused_where_it_could_not_be_fetched() {
        let upstream = script("https://github.com/ghostty-org/ghostty.git");
        assert!(repinned(&upstream, NEW, FORK).is_err(), "script fetches from another repository");
        assert!(repinned(&script(FORK), "0538f7535", FORK).is_err(), "short id");
        assert!(repinned("fn main() {}\n", NEW, FORK).is_err(), "no pin at all");
        let twice = format!("{}const GHOSTTY_COMMIT: &str = \"{OLD}\";\n", script(FORK));
        assert!(pinned(&twice).is_err(), "two pins");
    }

    /// The regenerated bindings may change what gen-bindings writes, the shared declarations and
    /// the targets' layout checks, either or both, and nothing else.
    #[test]
    fn only_what_gen_bindings_writes_is_taken() {
        let [bindings, layouts] = GENERATED;
        assert!(strays(&format!("{bindings}\n{layouts}\n")).is_empty(), "both");
        assert!(strays(layouts).is_empty(), "the layout checks alone");
        assert!(strays("").is_empty(), "nothing");
        let build = "crates/libghostty-vt-sys/build.rs";
        assert_eq!(strays(&format!("{bindings}\n{build}")), [build]);
        assert_eq!(
            strays("crates/libghostty-vt-sys/src/layouts.rs.orig"),
            ["crates/libghostty-vt-sys/src/layouts.rs.orig"]
        );
    }
}
